import type { HotkeyBinding } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';
import { saveSettings } from './settings';

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

/**
 * The name this window sends a hotkey under.
 *
 * `apps/recorder/src/settings.rs` prefixes every hotkey key with `hotkey_` so
 * that one `apply_settings` can carry a combination and a frame rate without
 * two vocabularies. The prefix is duplicated here rather than derived, because
 * the alternative is a protocol field whose only job is to spell a constant —
 * `apps/recorder/tests/ipc_protocol.rs` is where the two are held together.
 */
export const HOTKEY_SETTING_PREFIX = 'hotkey_';

/**
 * What a hotkey is spelled as when it is deliberately bound to nothing.
 *
 * `apps/recorder/src/settings.rs` calls this `UNBOUND`, and it is **not** the
 * same as clearing the setting. `null` is Reset: it stops the file saying
 * anything, so the action follows whatever Clipped ships with, today and after
 * the day that default changes. This word is the user saying "nothing", which
 * survives that change.
 *
 * The two are one keystroke apart in a settings screen and a year apart in
 * consequence, which is why the window has a control for each rather than one
 * labelled Clear.
 */
export const UNBOUND = 'none';

/** What a chord this window captured is called on the wire. */
export function hotkeySettingKey(action: string): string {
  return `${HOTKEY_SETTING_PREFIX}${action}`;
}

/**
 * The combination a key press names, or `null` when it names none yet.
 *
 * Windows binds a *modified* key, so a press of `Shift` alone is somebody part
 * way through a chord rather than a chord — returning `null` for it is what
 * lets a capture control wait instead of saving `Shift`.
 *
 * The spelling is `Hotkey`'s own (`Ctrl+Shift+F9`): modifiers in a fixed order
 * so the same chord is one string however it was typed, then the key. It has to
 * match, because the recorder parses this with `FromStr` and answers with
 * `Display` — a window that spelled it `Control+F9` would save a setting the
 * recorder refuses.
 */
export function chordOf(event: {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly shiftKey: boolean;
  readonly altKey: boolean;
}): string | null {
  const key = namedKey(event.key);
  if (key === null) {
    return null;
  }

  const parts: string[] = [];
  if (event.ctrlKey) {
    parts.push('Ctrl');
  }
  if (event.altKey) {
    parts.push('Alt');
  }
  if (event.shiftKey) {
    parts.push('Shift');
  }

  // A bare key is not a global hotkey. `RegisterHotKey` would take `F9` and
  // then every press of F9 in every application would belong to Clipped, which
  // is not a thing a user asked for by pressing one key in a settings screen.
  if (parts.length === 0) {
    return null;
  }

  parts.push(key);
  return parts.join('+');
}

/** The key half of a chord, or `null` when the press was a modifier alone. */
function namedKey(key: string): string | null {
  if (key === 'Control' || key === 'Shift' || key === 'Alt' || key === 'Meta') {
    return null;
  }
  if (/^F([1-9]|1\d|2[0-4])$/.test(key)) {
    return key;
  }
  if (/^[a-zA-Z0-9]$/.test(key)) {
    return key.toUpperCase();
  }
  return null;
}

/**
 * The action already holding `chord`, if this window can see one.
 *
 * "Where detectable" is the whole of what this claims, and issue #54's second
 * criterion says it that way for a reason. Two of Clipped's own actions on one
 * combination is a conflict this window can see in the list it already has, and
 * saving it would trade one working hotkey for another. A combination *another
 * application* owns is not visible from here at all — only the process that
 * calls `RegisterHotKey` learns that, which is the recorder, and it reports it
 * on the next read.
 */
export function actionHolding(
  rows: readonly HotkeyBinding[],
  chord: string,
  except: string,
): HotkeyBinding | null {
  return rows.find((row) => row.action !== except && row.hotkey === chord) ?? null;
}

/** Saving a combination, and what came back. */
export interface HotkeyEditor {
  /** The rows as they now stand, or `null` until the first read answers. */
  readonly rows: readonly HotkeyBinding[] | null;
  /** The action being saved, or `null` when nothing is in flight. */
  readonly pending: string | null;
  /** What went wrong with the last save, if anything did. */
  readonly problem: LibraryProblem | null;
  /** Binds `action` to `chord`, or unbinds it when `chord` is `null`. */
  readonly bind: (action: string, chord: string | null) => Promise<void>;
}

/**
 * Binding a combination from this window (issue #54).
 *
 * One save does the whole of it. `apply_settings` writes the combination into
 * the settings file *and* rebinds the running service — `apps/recorder/src/hotkeys.rs`
 * is where the second half happens, and it is why the first acceptance
 * criterion, "rebinding takes effect immediately", is a property of the
 * recorder rather than something this window arranges by asking twice
 * (issue #233, closed).
 *
 * What this window does have to do is **read the answer back**. The reply says
 * what the settings are, not what Windows made of them, and those differ
 * exactly when it matters: a combination another application owns saves
 * perfectly and then does not register. So a save is followed by a re-read, and
 * the row the user just changed shows the recorder's verdict rather than the
 * value that was sent.
 */
export function useHotkeyEditor(read: readonly HotkeyBinding[] | null): HotkeyEditor {
  const [edited, setEdited] = useState<readonly HotkeyBinding[] | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [problem, setProblem] = useState<LibraryProblem | null>(null);

  const bind = useCallback(async (action: string, chord: string | null): Promise<void> => {
    setPending(action);
    setProblem(null);
    try {
      await saveSettings({ [hotkeySettingKey(action)]: chord });
      // The verdict, not the request. See above.
      setEdited(await readHotkeys());
    } catch (thrown: unknown) {
      setProblem(asProblem(thrown));
    } finally {
      setPending(null);
    }
  }, []);

  return { rows: edited ?? read, pending, problem, bind };
}
