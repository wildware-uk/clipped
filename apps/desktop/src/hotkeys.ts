import type { HotkeyBinding } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';

/**
 * Reading the global hotkeys from the window.
 *
 * The window registers none of them. `RegisterHotKey` gives a combination to
 * exactly one process, and that process is the recorder: it is the one that
 * starts at login and outlives every window, and the one that can act on a press
 * (`docs/adr/0009-the-recorder-registers-global-hotkeys.md`, `docs/hotkeys.md`).
 *
 * That has a consequence this module exists for. The recorder finds out that
 * Discord already owns `Ctrl+F10` at the moment it registers, which may be days
 * before anybody opens this window — so a conflict cannot arrive as an event,
 * and a window that waited for one would show a clean list for ever. It is asked
 * for instead, whenever the screen that draws it is opened.
 *
 * # Three answers, never two
 *
 * The same three {@link LibraryRead} carries, and for the same reason: an empty
 * list is not the same as a recorder that could not be asked, and drawing the
 * second as the first would say "no hotkey has a problem" about a recorder that
 * was never reached (AGENTS.md section 27).
 */

/** Asks the recorder where every hotkey stands. */
export async function readHotkeys(): Promise<readonly HotkeyBinding[]> {
  return invoke<HotkeyBinding[]>('recorder_hotkeys');
}

/**
 * What the window says about a hotkey list it could not read.
 *
 * `unknown_command` is the one worth its own sentence. A recorder that started
 * before this window was installed registers no hotkeys at all and has no
 * `get_hotkeys` command, and "your hotkeys could not be read" would send
 * somebody looking for a fault in their keyboard rather than restarting Clipped
 * (AGENTS.md section 45).
 */
export function describeHotkeyProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder about your hotkeys, so nothing here is known. ${problem.message}`;
    case 'unknown_command':
      return 'The recorder that is running is older than this window and registers no hotkeys at all, so none of them does anything. Restarting Clipped starts the recorder that came with it.';
    default:
      return problem.message;
  }
}

/**
 * What one row says about whether that key does anything.
 *
 * Three states rather than two, because there are three ways a hotkey ends up
 * doing nothing and the useful action differs in each:
 *
 * - **unbound** — nothing is pointed at it. Not a fault; five of the seven
 *   actions start this way.
 * - **conflict** — Windows would not give Clipped the combination. The sentence
 *   is the recorder's own and names who is likely to have it.
 * - **unavailable** — the combination is registered and nothing in this build
 *   performs the action, so pressing it reports itself rather than working.
 */
export type HotkeyCondition = 'working' | 'unbound' | 'conflict' | 'unavailable';

/** Which of the four a row is in. */
export function conditionOf(row: HotkeyBinding): HotkeyCondition {
  if (row.state.state === 'conflict') {
    return 'conflict';
  }
  if (!row.handled) {
    return 'unavailable';
  }
  return row.state.state === 'unbound' ? 'unbound' : 'working';
}

/**
 * What that condition says, in the words shown beside the row.
 *
 * The recorder's own sentence wherever there is one: only the process that asked
 * Windows knows who has the combination, and only it knows which milestone would
 * build the action. This window invents no wording for either.
 */
export function describeCondition(row: HotkeyBinding): string {
  switch (conditionOf(row)) {
    case 'conflict':
      return row.state.state === 'conflict' ? row.state.reason : '';
    case 'unavailable':
      return row.unavailable ?? '';
    case 'unbound':
      return 'Not bound to anything.';
    case 'working':
      return 'Registered, and this build performs it.';
  }
}

/** The hotkeys, read once when the screen that shows them is opened. */
export function useHotkeys(): LibraryRead<readonly HotkeyBinding[]> {
  const [read, setRead] = useState<LibraryRead<readonly HotkeyBinding[]>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    readHotkeys()
      .then((hotkeys) => {
        if (current) {
          setRead({ state: 'read', value: hotkeys });
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
