import type { HotkeyBinding } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { conditionOf, describeCondition, describeHotkeyProblem } from './hotkeys';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The hotkey list on the Settings screen (issue #232).
 *
 * The property worth testing is not that a table renders. It is that the three
 * ways a hotkey ends up doing nothing are told apart, because they are told
 * apart nowhere else: `crates/hotkeys` has reported a conflict since issue #39
 * and had nothing to report it *to*, so the failure this screen exists to
 * prevent is a key that does nothing and a window that says everything is fine.
 *
 * - A combination another application owns.
 * - A combination that registered for an action nothing in this build performs.
 * - A recorder that could not be asked at all — which is not an empty list, and
 *   drawing it as one would be the fabricated state AGENTS.md section 27
 *   forbids.
 */

/**
 * A row, as the recorder sends one.
 *
 * `hotkey` and `unavailable` are absent from the wire rather than sent as
 * `undefined`, which `exactOptionalPropertyTypes` is right to insist on: a row
 * built here with `hotkey: undefined` would not be a row the recorder can
 * produce. So an unbound row omits the key instead.
 */
function binding(over: Partial<HotkeyBinding> = {}): HotkeyBinding {
  return {
    action: 'add_bookmark',
    label: 'Add bookmark',
    hotkey: 'Ctrl+F9',
    state: { state: 'registered' },
    handled: true,
    ...over,
  };
}

/** The same, with no combination bound to it. */
function unbound(over: Partial<HotkeyBinding> = {}): HotkeyBinding {
  const row = { ...binding(over), state: { state: 'unbound' } as const };
  delete (row as { hotkey?: string }).hotkey;
  return row;
}

/** The conflict sentence the recorder actually sends, abbreviated. */
const TAKEN =
  "Ctrl+F10 could not be Clipped's shortcut for Save replay: another application already uses " +
  'it. Choose a different combination, or close the application that has this one and try again';

/** What no build performs, in the recorder's words. */
const UNBUILT =
  'Save replay is not in this build: a recording with a replay buffer arrives in M3 (issue #38)';

/** Opens Settings, then its Hotkeys section, in the real window. */
async function openHotkeys(): Promise<HTMLElement> {
  const user = userEvent.setup();
  render(<App />);
  await user.click(screen.getByRole('link', { name: 'Settings' }));
  await user.click(screen.getByRole('tab', { name: 'Hotkeys' }));
  return screen.getByRole('region', { name: 'Global hotkeys' });
}

/** The cells of one action's row. */
function rowFor(list: HTMLElement, label: string): readonly string[] {
  const heading = within(list).getByRole('rowheader', { name: label });
  const row = heading.closest('tr');
  if (row === null) {
    throw new Error(`the "${label}" row header is not in a row`);
  }
  return within(row)
    .getAllByRole('cell')
    .map((cell) => cell.textContent ?? '');
}

describe('the hotkey list', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /*
   * The whole point, end to end: the window asks the recorder, and a combination
   * the user cannot have arrives on screen with the recorder's own sentence.
   * Before this, that sentence existed only in a log file.
   */
  it('shows a combination another application owns, in the recorder’s own words', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      recorderHotkeys: () => [
        binding({
          action: 'save_replay',
          label: 'Save replay',
          hotkey: 'Ctrl+F10',
          state: { state: 'conflict', reason: TAKEN },
          handled: false,
        }),
        binding(),
      ],
    });

    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    const [combination, state, meaning] = rowFor(list, 'Save replay');
    expect(combination).toBe('Ctrl+F10');
    expect(state).toBe('Taken');
    expect(meaning).toContain('another application already uses it');
    expect(meaning).toContain('Choose a different combination');
  });

  /*
   * Registered and useless are not the same thing, and this is the row that
   * would otherwise read as working: Windows accepts `Ctrl+F10` happily, and no
   * build saves a replay.
   */
  it('tells a registered combination apart from one whose action nothing performs', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      recorderHotkeys: () => [
        binding({
          action: 'save_replay',
          label: 'Save replay',
          hotkey: 'Ctrl+F10',
          state: { state: 'registered' },
          handled: false,
          unavailable: UNBUILT,
        }),
        binding(),
      ],
    });

    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    const [, unbuiltState, why] = rowFor(list, 'Save replay');
    expect(unbuiltState).toBe('Not in this build');
    expect(why).toContain('M3');
    expect(why).toContain('#38');

    const [, workingState] = rowFor(list, 'Add bookmark');
    expect(workingState).toBe('Ready');
  });

  /*
   * A recorder that could not be asked is not a recorder with no conflicts. The
   * table is absent and the reason is on screen; a table of nothing would say
   * "every hotkey is fine" about a question that was never answered.
   */
  it('says a recorder could not be asked, rather than drawing an untroubled list', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      recorderHotkeys: () => {
        throw { code: 'recorder_unreachable', message: 'the pipe was not there' };
      },
    });

    const list = await openHotkeys();
    await waitFor(() => {
      expect(list).toHaveTextContent(/could not ask the recorder about your hotkeys/i);
    });
    expect(within(list).queryByRole('table')).toBeNull();
  });

  /*
   * A recorder from before issue #232 registers nothing at all, and refuses the
   * command by name. "Your hotkeys could not be read" would send somebody
   * looking at their keyboard; the useful action is to restart Clipped.
   */
  it('tells a recorder older than this window what to do about itself', () => {
    const said = describeHotkeyProblem({
      code: 'unknown_command',
      message: 'this recorder has no `get_hotkeys` command',
    });

    expect(said).toContain('older than this window');
    expect(said).toContain('Restarting Clipped');
  });

  describe('the condition one row is in', () => {
    it('is a conflict before it is anything else', () => {
      // A conflicting binding is also unhandled when the action is unbuilt, and
      // the conflict is the one to show: the combination is the thing the user
      // can change.
      const row = binding({
        state: { state: 'conflict', reason: TAKEN },
        handled: false,
        unavailable: UNBUILT,
      });

      expect(conditionOf(row)).toBe('conflict');
      expect(describeCondition(row)).toBe(TAKEN);
    });

    it('is unavailable when the combination registered and nothing performs it', () => {
      const row = binding({ handled: false, unavailable: UNBUILT });

      expect(conditionOf(row)).toBe('unavailable');
      expect(describeCondition(row)).toBe(UNBUILT);
    });

    it('is unbound when nothing is pointed at it, which is not a fault', () => {
      const row = unbound();

      expect(conditionOf(row)).toBe('unbound');
      expect(describeCondition(row)).toBe('Not bound to anything.');
    });

    it('is working only when it registered and this build performs it', () => {
      expect(conditionOf(binding())).toBe('working');
    });
  });
});
