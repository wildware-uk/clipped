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

/**
 * Binding a combination from this window (issue #54).
 *
 * The recorder does the hard half: `apply_settings` writes the combination and
 * rebinds the running service in one call (issue #233, closed), so what is left
 * to test here is what this window sends, what it refuses to send, and what it
 * draws afterwards.
 *
 * The last of those is the one that matters most. A combination another
 * application owns *saves perfectly* and then fails to register, so a window
 * that drew what it sent would report success for a key that does nothing —
 * which is the exact failure issue #232 built this table to end.
 */
describe('binding a hotkey from this window', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /** Two rows, and a record of everything the window asked the recorder. */
  function stubbed(hotkeys?: () => readonly HotkeyBinding[]) {
    return stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      recorderHotkeys:
        hotkeys ??
        (() => [
          binding({ action: 'save_replay', label: 'Save replay', hotkey: 'Ctrl+F10' }),
          binding(),
        ]),
      applySettings: () => ({ settings: [] }),
    });
  }

  it('sends the combination that was pressed, under the key the recorder reads', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Change Save replay' }));
    await user.keyboard('{Control>}{Shift>}{F9}{/Shift}{/Control}');

    await waitFor(() => {
      const asked = runtime.invocations.filter(
        (call) => call.command === 'apply_recorder_settings',
      );
      expect(asked).toHaveLength(1);
      expect(asked[0]?.args).toMatchObject({ values: { hotkey_save_replay: 'Ctrl+Shift+F9' } });
    });
  });

  /*
   * The verdict, not the request. The save succeeds and the combination is
   * still refused by Windows, which is the case a window that drew what it sent
   * would get wrong — and it is the common one, because the combinations worth
   * rebinding onto are the ones other applications want too.
   */
  it('draws what the recorder made of the combination, not what was sent', async () => {
    let asked = 0;
    stubbed(() => {
      asked += 1;
      return asked === 1
        ? [binding({ action: 'save_replay', label: 'Save replay', hotkey: 'Ctrl+F10' })]
        : [
            binding({
              action: 'save_replay',
              label: 'Save replay',
              hotkey: 'Ctrl+Shift+F9',
              state: { state: 'conflict', reason: TAKEN },
              handled: false,
            }),
          ];
    });
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Change Save replay' }));
    await user.keyboard('{Control>}{Shift>}{F9}{/Shift}{/Control}');

    await waitFor(() => {
      const [combination, state, meaning] = rowFor(list, 'Save replay');
      expect(combination).toBe('Ctrl+Shift+F9');
      expect(state).toBe('Taken');
      expect(meaning).toContain('another application already uses it');
    });
  });

  /*
   * Issue #54's second criterion, the half this window can actually see: two of
   * Clipped's own actions on one combination. Saving it would trade a working
   * hotkey for another and the user would find out by pressing the old one.
   */
  it('refuses a combination another Clipped action already holds, before sending it', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    // `Ctrl+F9` is Add bookmark's, in the rows above.
    await user.click(within(list).getByRole('button', { name: 'Change Save replay' }));
    await user.keyboard('{Control>}{F9}{/Control}');

    await waitFor(() => {
      expect(rowFor(list, 'Save replay')[2]).toContain('already Add bookmark');
    });
    expect(
      runtime.invocations.filter((call) => call.command === 'apply_recorder_settings'),
    ).toHaveLength(0);
    // And the row still holds what it held.
    expect(rowFor(list, 'Save replay')[0]).toBe('Ctrl+F10');
  });

  /*
   * A modifier on its own is somebody part way through a chord. Saving `Shift`
   * would be this window inventing an instruction out of a key that was on its
   * way to being half of one.
   */
  it('keeps waiting while only a modifier is down', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Change Save replay' }));
    await user.keyboard('{Shift>}');

    expect(within(list).getByRole('button', { name: /Press a combination/ })).toBeVisible();
    expect(
      runtime.invocations.filter((call) => call.command === 'apply_recorder_settings'),
    ).toHaveLength(0);
  });

  it('stops listening on Escape without binding anything', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Change Save replay' }));
    await user.keyboard('{Escape}');

    await waitFor(() => {
      expect(within(list).getByRole('button', { name: 'Change Save replay' })).toBeVisible();
    });
    expect(
      runtime.invocations.filter((call) => call.command === 'apply_recorder_settings'),
    ).toHaveLength(0);
  });

  /*
   * Unbinding and resetting are two things and the window sends two different
   * values for them. `apps/recorder/src/settings.rs` is explicit:
   *
   * > Clearing it with `null` is Reset, which is a different thing — it stops
   * > the file saying anything, so the action follows whatever Clipped ships
   * > with, today and after the day the default changes.
   *
   * So a window that sent `null` for "unbind" would give somebody `Ctrl+F10`
   * back the day Clipped changed what it ships with, having been asked for no
   * hotkey at all. These two cases are what keep the pair apart, and neither
   * could be caught by looking at this window alone.
   */
  it('unbinds with the word the recorder reads, not by clearing the setting', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Unbind Save replay' }));

    await waitFor(() => {
      const asked = runtime.invocations.filter(
        (call) => call.command === 'apply_recorder_settings',
      );
      expect(asked).toHaveLength(1);
      expect(asked[0]?.args).toMatchObject({ values: { hotkey_save_replay: 'none' } });
    });
  });

  it('resets to the shipped default by clearing the setting', async () => {
    const runtime = stubbed();
    const user = userEvent.setup();
    const list = await openHotkeys();
    await waitFor(() => {
      expect(within(list).getByRole('table')).toBeVisible();
    });

    await user.click(within(list).getByRole('button', { name: 'Reset Save replay' }));

    await waitFor(() => {
      const asked = runtime.invocations.filter(
        (call) => call.command === 'apply_recorder_settings',
      );
      expect(asked).toHaveLength(1);
      expect(asked[0]?.args).toMatchObject({ values: { hotkey_save_replay: null } });
    });
  });
});
